
# Big picture

We are creating a HDL language, currently named mirin.

## Key ideas

- RTL language, want to be able to express hardware correctly at the register transfer level, along with features like clock crossing, reset etc.
- Readability is of high importance
- Generate high quality verilog, with consistent naming and the ability to expressily force naming if required.
- The language is typed at two levels:
   - Type checking at library level, which ensures basic compatibility of types, and allows for type inference.
   - Type checking at instantiation time, which allows more complicated type checking, such as checking that the width of a signal matches the width of a port, or that the types of two signals are compatible for an operation.
- "Ports" or "interfaces"
  - Ports are a first class concept in the language, and can be used to define the interface of a module, as well as to connect modules together.
  - Fields of a port have an input and output direction
  - Parameters can be contained inside a port
  - Data should stay in positive positions where sensible; a write input is a notable exception.
  - Passing a port in an argument position may still need an explicit direction annotation such as `out downstream`.
- structs
  - structs are supported. Use similar syntax to ports but do have input/output annotations.
  - structs are strictly positive, unlike ports which can contain input fields.
- arrays / vecs
   - arrays are strictly positive, and must be a fixed size
   - vecs can contain ports, unlike arrays, also must be a fixed size
   - test time versions exist which can be of variable size.
- domains / metadata
  - for now, only clock domains are in scope for the type system
  - types can have an added clock domain, which determines the clock they are associated with
  - clocking is checked when signals are connected
  - reset values are also clock-associated, e.g. `Reset @clk`
  - generalized metadata such as pipeline delay is deferred
- Testing needs to be integrated into the language, with the ability to write testbenches and run simulations directly from the language.


## Things to think about

- big endian vs little endian, how to specify the order of bits in a signal
- syntax for metadata
- how inferable arguments like `dom clk` should work, especially once generics are introduced


Example multAdd
``` rust
fn multAdd // fn introduces a component
  // keyword named section (optional):
  { dom clk: Clock, // # indicates this can be elided when instantiating
    rstn: Reset @clk = high, // reset associated with clk, default active value
    c: uint[8] @clk = 0, // a uint of width 8, associated with the clock domain clk, with a default value of 0
  }
  // further arguments (optional):
  ( a: uint[8] @clk, b: uint[8] @clk
  )
  -> uint[8] @clk // return type.
  // body
  {
    let mult = a * b; // type inference, mult is inferred to be uint[16] @clk
    let mult = mult[8:0]; // slice
    let mult = mult.reg{rstn}(); // register, automatically use associated clk
    let add = mult + c; // type inference, add is inferred to be uint[8] @clk
    return add;
  }

```

Example counter
``` rust
fn counter
  { dom clk: Clock, rstn: Reset @clk = high }
  ( param bits: usize )
  -> uint[bits] @clk
  {
    // Doesn't make sense at the moment
    // Challenge: find a good way to support the recursive conneciton of the counter to
    // itself, while keeping an imperative style elsewhere.
    let count; // type inference, count is inferred to be uint[bits] @clk
    count = count + 1; // type inference, count is inferred to be uint[bits] @clk
    count = count.reg{rstn, reset_val = 0}(); // register, automatically use associated clk

    // current direction: use var for block-scoped feedback signals
    var count: uint[bits] @clk;
    count = (count + 1).reg{rstn, reset_val = 0}();
    return count;
    // Note: an earlier candidate used `rec count = { ... }` for cyclic definitions.
    // That has been superseded by `var`. See planning/cycles_and_scoping.md.
  }

  Metadata questions:

  - like the @clk syntax, but if we want multiple metadata how would that look?
    - @(clk = clk) // equivalent to @clk, with @(clk = clk1) to specifiy a different clk

  - how additional metadata interacts with functionality like reg
  - especially if user defined like "pipeline_delay"
  - could user add additional info to existing function for their metadata?
  - Or do we just need a new version of reg for new combination of metadata? Could get out of hand.

Current working syntax subset is tracked in `planning/syntax.md`.
