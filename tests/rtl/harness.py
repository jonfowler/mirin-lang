"""Shared glue for the cocotb RTL tests.

Each test compiles a `.plr` example with the polar compiler, then builds the
generated SystemVerilog under Verilator and runs cocotb coroutines against it
via cocotb's Python runner. Test files pair a pytest entry point (which calls
`simulate`) with `@cocotb.test()` coroutines in the same module — the runner
re-imports the module inside the simulator process and picks up the
coroutines.
"""

import subprocess
from pathlib import Path

from cocotb_tools.runner import get_runner

REPO = Path(__file__).resolve().parents[2]
EXAMPLES = REPO / "examples" / "working"
BUILD = Path(__file__).resolve().parent / "build"


def compile_plr(stem: str) -> Path:
    """Compile `examples/working/<stem>.plr` and return the emitted SV path."""
    src = EXAMPLES / f"{stem}.plr"
    subprocess.run(
        ["cargo", "run", "-q", "-p", "polar-compiler", "--", str(src)],
        cwd=REPO,
        check=True,
    )
    return REPO / "sv" / f"{stem}.sv"


def simulate(
    stem: str,
    top: str,
    test_module: str,
    parameters: dict | None = None,
    expect_sim_stop: bool = False,
):
    """Build `top` from the example's generated SV and run the cocotb tests
    in `test_module` against it. `parameters` bind the module's SV parameters
    (Polar const generics) at elaboration.

    `expect_sim_stop`: a test deliberately trips a $stop (e.g. proving a
    bounds assertion fires), so the simulator exits nonzero even though
    every cocotb test passed - fall back to judging by the results file."""
    sv = compile_plr(stem)
    suffix = "_".join(str(v) for v in (parameters or {}).values())
    build_dir = BUILD / (f"{stem}_{top}" + (f"_{suffix}" if suffix else ""))
    runner = get_runner("verilator")
    runner.build(
        sources=[sv],
        hdl_toplevel=top,
        parameters=parameters or {},
        build_dir=build_dir,
        always=True,
        # Evaluate immediate assertions (dynamic-index bounds checks,
        # elaboration-time fit/width residuals) during simulation.
        build_args=["--assert"],
    )
    try:
        runner.test(hdl_toplevel=top, test_module=test_module, build_dir=build_dir)
    except SystemExit:
        if not expect_sim_stop:
            raise
        from cocotb_tools.runner import get_results

        # Under pytest the runner writes `<test name>.result.xml`.
        candidates = sorted(build_dir.glob("*.xml"), key=lambda f: f.stat().st_mtime)
        assert candidates, f"no results xml in {build_dir}"
        num_tests, num_failed = get_results(candidates[-1])
        assert num_failed == 0, f"{num_failed} of {num_tests} cocotb tests failed"
