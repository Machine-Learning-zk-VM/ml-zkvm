# Machine Learning zk-VM Compiler

<img src="logo.png" width="200px"/>

Fork of [zkonduit/ezkl](https://github.com/zkonduit/ezkl), hijacked for a Hackathon project, see [Devfolio submission](https://devfolio.co/projects/machine-learning-zkvm-fdf6) and [this presentation](https://docs.google.com/presentation/d/11AauxQ1tfuM5U18hWUpqz-ph3awOmvonee1DjoijYF0/edit?usp=sharing).

## Setup

Install [Powdr](https://www.powdr.org/):
```bash
git clone git@github.com:powdr-labs/powdr.git
cd powdr
cargo install --path powdr_cli --features halo2
```

## Compiling a VM

Go inside a directory with a `network.onnx` & `input.json`, for example:
- `examples/onnx/2l_relu_fc`
- `examples/onnx/4l_relu_conv_fc`

Run the following EZKL steps to prepare the network:
```bash
cargo run gen-settings -M network.onnx --param-visibility public --input-visibility private --output-visibility private --logrows 16
cargo run compile-circuit -M network.onnx -S settings.json --compiled-circuit network.ezkl
cargo run gen-witness -M network.ezkl -D input.json
```

Run the `generate-program` command added by this repository:
```bash
cargo run generate-program -M network.ezkl --witness witness.json
```

This will generate 2 files:
- `vm.asm`: The model-agnostic [Powdr VM](src/powdr_template.asm) with the model-specific assembly program inserted
- `memory.csv`: The content of the memory, specific to the input (from `input.json`)

These can be passed to Powdr to compile the VM and make a prove of the program:
```bash
powdr pil vm.asm -f --witness-values memory.csv --prove-with estark
```

(or without the `--prove-with estark` flag if you just want to check everything works)

For a better comparison with EZKL, you can also create the proof with the Halo2 backend:
```bash
powdr pil vm.asm -f --witness-values memory.csv --prove-with halo2 --field bn254
```

For comparison, to benchmark EZKL:
```bash
cargo run -r get-srs --logrows=16 --srs-path=16.srs
cargo run -r setup -M network.ezkl --srs-path=16.srs
time cargo run -r prove -M network.ezkl --witness witness.json --pk-path=pk.key --proof-path=model.proof --srs-path=16.srs
```
