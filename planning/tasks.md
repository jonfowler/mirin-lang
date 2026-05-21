

Perform the following tasks:

- setup basic syntax highlighting for current syntax in a sensible way to be used with VSCode
- come up with a starting architecture for the compiler
    - think about different stages.
    - think about a minimal core language
    - consider tree sitter as a parser
    - compiler will be written in rust
    - think about different intermediate representations and compiler stages
    - note we don't need to worry about optimizing as that is handled by downstream tools, we just need to generate verilog that is correct and high quality.
- should have "impl" similar to rust, which allows for methods on types. come up with some example syntax for this.
- write these out into a high-level design document.
- write a first go at a parser for a small portion of the syntax
