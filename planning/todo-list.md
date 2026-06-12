# Todo list

## Done
- module system
- field assignment e.g. df.valid = true;
- inline verilog
- story about testing
   - v1 is probably using open source verilator with driver such as cocotb
     - DONE (2026-06): `tests/rtl/` — cocotb 2.0 + verilator behavioural suite over the working examples (`tests/rtl/run.sh`)
- traits
- numeric literals


## List of things we want in the language

- "crate" system
- support for wide range of primitives
- support for vectors, indexing, slicing, "for constructs"
- support for higher level functions!
- support for "let mut", to allow for wide range of for looping.
 deriving "pack instances" for structs, to allow for easy packing and unpacking of structs into bitvectors.
- licensing, docs, publish
- sim models vs synthesis models, to allow us to support stuff like Xilinx/Altera IP by providing a sim model for them and checking against that sim model in modelsim.
- Optional: verilog pragma support
- Optional: explicitly named modules
- LONGER TERM: correct handling of linearity given the existence of ports e.g.
      `fn dup(x : T) -> (T, T)` is not valid if `T` is a port type, but is valid if `T` is a non-port type. Ignore for now (could check at monomorphisation time).
- LONGER TERM: dependency typing to avoid combinatorial loops. e.g. ready is allowed to combinatorial depend on valid but not the other way around.
- story about testing
  - LONGER TERM: note there should be a longer term aspiration to push towards E2E testing within the language itself
