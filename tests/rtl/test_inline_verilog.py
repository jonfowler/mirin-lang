"""inline_verilog.plr: hand-written enabled flop behind a Polar signature —
checks the verbatim body and its `${…}` splices actually simulate."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

from harness import simulate


@cocotb.test()
async def enabled_flop(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.en.value = 0
    dut.d.value = 0
    await FallingEdge(dut.clk)
    q = int(dut.result.value)  # no reset; verilator initialises regs to 0

    rng = random.Random(2)
    for _ in range(50):
        en, d = rng.randrange(2), rng.randrange(256)
        dut.en.value = en
        dut.d.value = d
        await RisingEdge(dut.clk)
        if en:
            q = d
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == q


def test_inline_verilog():
    simulate("inline_verilog", "top", "test_inline_verilog")
