"""ram.plr: the inferred RAM — write with enable, read back, holds."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge

from harness import simulate


@cocotb.test()
async def writes_reads_and_holds(dut):
    Clock(dut.clk, 10, unit="ns").start()
    model = [0] * 4
    dut.we.value = 1
    rng = random.Random(5)
    # Fill all four words.
    for a in range(4):
        d = rng.randrange(256)
        dut.waddr.value = a
        dut.wdata.value = d
        await FallingEdge(dut.clk)
        model[a] = d
    # Disable writes; every word must hold and read back.
    dut.we.value = 0
    dut.wdata.value = 0xAA
    for a in range(4):
        dut.raddr.value = a
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == model[a]
    # Sparse overwrite with reads interleaved.
    dut.we.value = 1
    for _ in range(20):
        a, d, r = rng.randrange(4), rng.randrange(256), rng.randrange(4)
        dut.waddr.value = a
        dut.wdata.value = d
        dut.raddr.value = r
        await FallingEdge(dut.clk)
        model[a] = d
        assert int(dut.result.value) == model[r]


def test_ram():
    simulate("ram", "ram", "test_ram")
