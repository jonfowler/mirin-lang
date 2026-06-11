# RTL tests

Behavioural simulation of the generated SystemVerilog: cocotb drives stimulus
from Python coroutines, Verilator compiles the SV into a fast 2-state
simulator. This complements the `-Wall` lint in the cargo example harness —
lint proves the SV elaborates; these tests prove it *computes the right thing*.

```bash
tests/rtl/run.sh            # bootstrap venv on first run, then pytest
tests/rtl/run.sh -k counter # one test
```

Requires `verilator`, `python3`, and a C++ toolchain (`make`, `g++`) on PATH.

## Shape

Each `test_<example>.py` pairs a pytest entry point with `@cocotb.test()`
coroutines in the same file. The pytest function calls `harness.simulate`,
which:

1. compiles `examples/working/<stem>.plr` with the polar compiler
   (`cargo run -p polar-compiler`) → `sv/<stem>.sv`,
2. builds the chosen top module under Verilator (binding SV parameters for
   parametric tops, e.g. `counter` at `bits=3` and `bits=8`),
3. runs the file's cocotb coroutines against it.

Build artifacts land in `tests/rtl/build/` (gitignored, keyed by
example/top/parameters).

## Conventions

- Drive inputs just after a falling edge, let the rising edge register them,
  assert on the next falling edge — keeps the tests free of delta-cycle
  subtleties.
- Fixed RNG seeds: failures reproduce exactly.
- Designs without a reset rely on Verilator's 2-state zero-initialisation;
  say so in a comment when a test does.
