use std::str::FromStr;

use thiserror::Error;

use halo2_proofs::{
    circuit::Layouter,
    plonk::{ConstraintSystem, Constraints, Expression, Selector},
    poly::Rotation,
};
use log::debug;
#[cfg(feature = "python-bindings")]
use pyo3::{
    conversion::{FromPyObject, PyTryFrom},
    exceptions::PyValueError,
    prelude::*,
    types::PyString,
};
use serde::{Deserialize, Serialize};

use crate::{
    circuit::ops::base::BaseOp,
    circuit::{table::Table, utils},
    tensor::{Tensor, TensorType, ValTensor, VarTensor},
};
use std::{collections::BTreeMap, error::Error, marker::PhantomData};

use super::{lookup::LookupOp, region::RegionCtx, Op};
use halo2curves::ff::{Field, PrimeField};

/// circuit related errors.
#[derive(Debug, Error)]
pub enum CircuitError {
    /// Shape mismatch in circuit construction
    #[error("dimension mismatch in circuit construction for op: {0}")]
    DimMismatch(String),
    /// Error when instantiating lookup tables
    #[error("failed to instantiate lookup tables")]
    LookupInstantiation,
    /// A lookup table was was already assigned
    #[error("attempting to initialize an already instantiated lookup table")]
    TableAlreadyAssigned,
    /// This operation is unsupported
    #[error("unsupported operation in graph")]
    UnsupportedOp,
}

#[allow(missing_docs)]
/// An enum representing activating the sanity checks we can perform on the accumulated arguments
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default, Copy,
)]
pub enum CheckMode {
    #[default]
    SAFE,
    UNSAFE,
}

impl From<String> for CheckMode {
    fn from(value: String) -> Self {
        match value.to_lowercase().as_str() {
            "safe" => CheckMode::SAFE,
            "unsafe" => CheckMode::UNSAFE,
            _ => panic!("not a valid checkmode"),
        }
    }
}

#[allow(missing_docs)]
/// An enum representing the tolerance we can accept for the accumulated arguments, either absolute or percentage
#[derive(Clone, Default, Debug, PartialEq, PartialOrd, Serialize, Deserialize, Copy)]
pub struct Tolerance {
    pub val: f32,
    pub scale: utils::F32,
}

impl FromStr for Tolerance {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(val) = s.parse::<f32>() {
            Ok(Tolerance {
                val,
                scale: utils::F32(1.0),
            })
        } else {
            Err(
                "Invalid tolerance value provided. It should expressed as a percentage (f32)."
                    .to_string(),
            )
        }
    }
}

impl From<f32> for Tolerance {
    fn from(value: f32) -> Self {
        Tolerance {
            val: value,
            scale: utils::F32(1.0),
        }
    }
}

#[cfg(feature = "python-bindings")]
/// Converts CheckMode into a PyObject (Required for CheckMode to be compatible with Python)
impl IntoPy<PyObject> for CheckMode {
    fn into_py(self, py: Python) -> PyObject {
        match self {
            CheckMode::SAFE => "safe".to_object(py),
            CheckMode::UNSAFE => "unsafe".to_object(py),
        }
    }
}

#[cfg(feature = "python-bindings")]
/// Obtains CheckMode from PyObject (Required for CheckMode to be compatible with Python)
impl<'source> FromPyObject<'source> for CheckMode {
    fn extract(ob: &'source PyAny) -> PyResult<Self> {
        let trystr = <PyString as PyTryFrom>::try_from(ob)?;
        let strval = trystr.to_string();
        match strval.to_lowercase().as_str() {
            "safe" => Ok(CheckMode::SAFE),
            "unsafe" => Ok(CheckMode::UNSAFE),
            _ => Err(PyValueError::new_err("Invalid value for CheckMode")),
        }
    }
}

#[cfg(feature = "python-bindings")]
/// Converts Tolerance into a PyObject (Required for Tolerance to be compatible with Python)
impl IntoPy<PyObject> for Tolerance {
    fn into_py(self, py: Python) -> PyObject {
        (self.val, self.scale.0).to_object(py)
    }
}

#[cfg(feature = "python-bindings")]
/// Obtains Tolerance from PyObject (Required for Tolerance to be compatible with Python)
impl<'source> FromPyObject<'source> for Tolerance {
    fn extract(ob: &'source PyAny) -> PyResult<Self> {
        if let Ok((val, scale)) = ob.extract::<(f32, f32)>() {
            Ok(Tolerance {
                val,
                scale: utils::F32(scale),
            })
        } else {
            Err(PyValueError::new_err("Invalid tolerance value provided. "))
        }
    }
}

/// Configuration for an accumulated arg.
#[derive(Clone, Debug, Default)]
pub struct BaseConfig<F: PrimeField + TensorType + PartialOrd> {
    /// the inputs to the accumulated operations.
    pub inputs: Vec<VarTensor>,
    /// the VarTensor reserved for lookup operations (could be an element of inputs)
    /// Note that you should be careful to ensure that the lookup_input is not simultaneously assigned to by other non-lookup operations eg. in the case of composite ops.
    pub lookup_input: VarTensor,
    /// the (currently singular) output of the accumulated operations.
    pub output: VarTensor,
    /// the VarTensor reserved for lookup operations (could be an element of inputs or the same as output)
    /// Note that you should be careful to ensure that the lookup_output is not simultaneously assigned to by other non-lookup operations eg. in the case of composite ops.
    pub lookup_output: VarTensor,
    /// [Selector]s generated when configuring the layer. We use a [BTreeMap] as we expect to configure [BaseOp].
    pub selectors: BTreeMap<(BaseOp, usize), Selector>,
    /// [Selector]s generated when configuring the layer. We use a [BTreeMap] as we expect to configure many lookup ops.
    pub lookup_selectors: BTreeMap<(LookupOp, usize), Selector>,
    ///
    pub tables: BTreeMap<LookupOp, Table<F>>,
    /// Activate sanity checks
    pub check_mode: CheckMode,
    _marker: PhantomData<F>,
}

impl<F: PrimeField + TensorType + PartialOrd> BaseConfig<F> {
    /// Returns a new [BaseConfig] with no inputs, no selectors, and no tables.
    pub fn dummy(col_size: usize) -> Self {
        Self {
            inputs: vec![VarTensor::dummy(col_size), VarTensor::dummy(col_size)],
            lookup_input: VarTensor::dummy(col_size),
            output: VarTensor::dummy(col_size),
            lookup_output: VarTensor::dummy(col_size),
            selectors: BTreeMap::new(),
            lookup_selectors: BTreeMap::new(),
            tables: BTreeMap::new(),
            check_mode: CheckMode::SAFE,
            _marker: PhantomData,
        }
    }

    /// Configures [BaseOp]s for a given [ConstraintSystem].
    /// # Arguments
    /// * `meta` - The [ConstraintSystem] to configure the operations in.
    /// * `inputs` - The explicit inputs to the operations.
    /// * `output` - The variable representing the (currently singular) output of the operations.
    /// * `check_mode` - The variable representing the (currently singular) output of the operations.
    pub fn configure(
        meta: &mut ConstraintSystem<F>,
        inputs: &[VarTensor; 2],
        output: &VarTensor,
        check_mode: CheckMode,
    ) -> Self {
        // setup a selector per base op
        let mut selectors = BTreeMap::new();

        assert!(inputs[0].num_cols() == inputs[1].num_cols());
        assert!(inputs[0].num_cols() == output.num_cols());

        for i in 0..output.num_cols() {
            selectors.insert((BaseOp::Add, i), meta.selector());
            selectors.insert((BaseOp::Sub, i), meta.selector());
            selectors.insert((BaseOp::Dot, i), meta.selector());
            selectors.insert((BaseOp::CumProd, i), meta.selector());
            selectors.insert((BaseOp::Sum, i), meta.selector());
            selectors.insert((BaseOp::Neg, i), meta.selector());
            selectors.insert((BaseOp::Mult, i), meta.selector());
            selectors.insert((BaseOp::IsZero, i), meta.selector());
            selectors.insert((BaseOp::Identity, i), meta.selector());
            selectors.insert((BaseOp::IsBoolean, i), meta.selector());
        }

        for ((base_op, col_idx), selector) in selectors.iter() {
            meta.create_gate(base_op.as_str(), |meta| {
                let selector = meta.query_selector(*selector);
                let idx_offset = col_idx * output.col_size();
                let mut qis = vec![Expression::<F>::zero().unwrap(); 2];
                for (i, q_i) in qis
                    .iter_mut()
                    .enumerate()
                    .take(2)
                    .skip(2 - base_op.num_inputs())
                {
                    *q_i = inputs[i]
                        .query_rng(meta, 0, idx_offset, 1)
                        .expect("accum: input query failed")[0]
                        .clone()
                }

                // Get output expressions for each input channel
                let (rotation_offset, rng) = base_op.query_offset_rng();

                let constraints = match base_op {
                    BaseOp::IsBoolean => {
                        vec![(qis[1].clone()) * (qis[1].clone() - Expression::Constant(F::from(1)))]
                    }
                    BaseOp::IsZero => vec![qis[1].clone()],
                    _ => {
                        let expected_output: Tensor<Expression<F>> = output
                            .query_rng(meta, rotation_offset, idx_offset, rng)
                            .expect("poly: output query failed");

                        let res =
                            base_op.f((qis[0].clone(), qis[1].clone(), expected_output[0].clone()));
                        vec![expected_output[base_op.constraint_idx()].clone() - res]
                    }
                };

                Constraints::with_selector(selector, constraints)
            });
        }

        Self {
            selectors,
            lookup_selectors: BTreeMap::new(),
            inputs: inputs.to_vec(),
            lookup_input: VarTensor::Empty,
            lookup_output: VarTensor::Empty,
            tables: BTreeMap::new(),
            output: output.clone(),
            check_mode,
            _marker: PhantomData,
        }
    }

    /// Configures and creates lookup selectors
    pub fn configure_lookup(
        &mut self,
        cs: &mut ConstraintSystem<F>,
        input: &VarTensor,
        output: &VarTensor,
        bits: usize,
        logrows: usize,
        nl: &LookupOp,
    ) -> Result<(), Box<dyn Error>>
    where
        F: Field,
    {
        let mut selectors = BTreeMap::new();

        // we borrow mutably twice so we need to do this dance

        let table = if !self.tables.contains_key(nl) {
            // as all tables have the same input we see if there's another table who's input we can reuse
            let table = if let Some(table) = self.tables.values().next() {
                Table::<F>::configure(cs, bits, logrows, nl, Some(table.table_inputs.clone()))
            } else {
                Table::<F>::configure(cs, bits, logrows, nl, None)
            };
            self.tables.insert(nl.clone(), table.clone());
            table
        } else {
            return Ok(());
        };

        for x in 0..input.num_cols() {
            let qlookups: Vec<Selector> = (0..table.table_inputs.len())
                .map(|_| cs.complex_selector())
                .collect();

            cs.lookup("", |cs| {
                let qlookups: Vec<Expression<F>> = (0..qlookups.len())
                    .map(|i| cs.query_selector(qlookups[i]).clone())
                    .collect();
                let not_qlookups: Vec<Expression<F>> = (0..qlookups.len())
                    .map(|i| Expression::Constant(<F as Field>::ONE) - qlookups[i].clone())
                    .collect();
                let (default_x, default_y): (F, F) = nl.default_pair();
                let mut res = vec![];

                for (i, (input_col, output_col)) in table
                    .table_inputs
                    .clone()
                    .into_iter()
                    .zip(table.table_outputs.clone().into_iter())
                    .enumerate()
                {
                    let qlookup = qlookups[i].clone();
                    let not_qlookup = not_qlookups[i].clone();
                    res.extend([
                        (
                            match &input {
                                VarTensor::Advice { inner: advices, .. } => {
                                    qlookup.clone() * cs.query_advice(advices[x], Rotation(0))
                                        + not_qlookup.clone() * default_x
                                }
                                _ => panic!("wrong input type"),
                            },
                            input_col,
                        ),
                        (
                            match &output {
                                VarTensor::Advice { inner: advices, .. } => {
                                    qlookup.clone() * cs.query_advice(advices[x], Rotation(0))
                                        + not_qlookup.clone() * default_y
                                }
                                _ => panic!("wrong output type"),
                            },
                            output_col,
                        ),
                    ]);
                }

                res
            });
            let aggregate_selector = cs.selector();
            cs.create_gate("", |cs| {
                let mut start = Expression::Constant(<F as Field>::ONE);
                for selector in qlookups.iter() {
                    let selector = cs.query_selector(*selector);
                    start = start * selector.clone();
                }
                vec![start]
            });

            selectors.insert((nl.clone(), x), aggregate_selector);
        }
        self.lookup_selectors.extend(selectors);
        // if we haven't previously initialized the input/output, do so now
        if let VarTensor::Empty = self.lookup_input {
            debug!("assigning lookup input");
            self.lookup_input = input.clone();
        }
        if let VarTensor::Empty = self.lookup_output {
            debug!("assigning lookup output");
            self.lookup_output = output.clone();
        }
        Ok(())
    }

    /// layout_tables must be called before layout.
    pub fn layout_tables(&mut self, layouter: &mut impl Layouter<F>) -> Result<(), Box<dyn Error>> {
        for (i, table) in self.tables.values_mut().enumerate() {
            if !table.is_assigned {
                debug!(
                    "laying out table for {}",
                    crate::circuit::ops::Op::<F>::as_string(&table.nonlinearity)
                );
                if i == 0 {
                    table.layout(layouter, false)?;
                } else {
                    table.layout(layouter, true)?;
                }
            }
        }
        Ok(())
    }

    /// Assigns variables to the regions created when calling `configure`.
    /// # Arguments
    /// * `values` - The explicit values to the operations.
    /// * `layouter` - A Halo2 Layouter.
    /// * `op` - The operation being represented.
    pub fn layout(
        &mut self,
        region: &mut RegionCtx<F>,
        values: &[ValTensor<F>],
        op: Box<dyn Op<F>>,
    ) -> Result<Option<ValTensor<F>>, Box<dyn Error>> {
        let res = op.layout(self, region, values)?;

        if matches!(&self.check_mode, CheckMode::SAFE) && !region.is_dummy() {
            if let Some(claimed_output) = &res {
                // during key generation this will be unknown vals so we use this as a flag to check
                let mut is_assigned = !claimed_output.any_unknowns();
                for val in values.iter() {
                    is_assigned = is_assigned && !val.any_unknowns();
                }
                if is_assigned {
                    op.safe_mode_check(claimed_output, values)?;
                }
            }
        };
        Ok(res)
    }
}
