"""vectors.plr: vec construction + dynamic (mux) indexing, and the
dynamic-index bounds assertion actually firing on an out-of-range access
(the harness builds with --assert)."""

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge
from cocotb.result import SimFailure

from harness import simulate


@cocotb.test()
async def mux_picks_elements(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.a.value = 3
    dut.b.value = 7
    dut.raw.value = 0
    # v = [a, b, 5]; result = v[0] + v[sel] + z[1] = a + v[sel] + 0
    for sel, elem in [(0, 3), (1, 7), (2, 5)]:
        dut.sel.value = sel
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == (3 + elem) % 16


@cocotb.test(expect_error=SimFailure)
async def out_of_range_index_fires_the_assert(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.a.value = 3
    dut.b.value = 7
    dut.raw.value = 0
    dut.sel.value = 3  # Vec(3, _): valid indexes are 0..2
    await FallingEdge(dut.clk)
    await FallingEdge(dut.clk)


def test_vectors():
    simulate("vectors", "f", "test_vectors", expect_sim_stop=True)
