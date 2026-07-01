# Compiler backend

## High-level overview

Aims:
- Have a clear rust representation of the Verilog IR, with a clear mapping to the source-level HIR.
- As much as possible, keep a 1-to-1 mapping between polar constructs and verilog constructs.

### Ports

Ports are collapsed into separate fields which are passed in and out of a verilog
module. Need to determine a good "delimiter" for port fields.

e.g.
```
fn df_sink{#clk}(x : df(uint(8)) @clk) {
   x.ready = false;
}
```
might go to
```sv
module df_sink (
   input logic clk,
   input logic [7:0] x_bits,
   input logic x_valid,
   output logic x_ready
);
   // body
```

But `_` is not a particularly good delimiter, since it's commonly used in identifiers.

TODO: investigate systemverilog spec to find a good delimiter.


### Structs

It is tempting to use verilog structs to represent polar structs. However, polar structs support parametric fields, which verilog structs do not. So these should
be flattened into separate fields, similar to ports.

### Instance names

Instance names for now should be taken from the name of the component, if multiple components of the same type are instantiated, we can add a numeric suffix to disambiguate them.

### Expressions vs code blocks

Operators such as `+` and `*` can be directly mapped to verilog operators/functions.

The current version of `reg` can be mapped to a module with a size parameter.
