"""ram.mrn: the functional RAM — init contents visible at power-on,
replace-based writes with enable, holds under we=0."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge

from harness import simulate


@cocotb.test()
async def init_writes_reads_and_holds(dut):
    Clock(dut.clk, 10, unit="ns").start()
    model = [0x10, 0x20, 0x30, 0x40]  # the `init` contents
    dut.we.value = 0
    dut.wdata.value = 0
    dut.waddr.value = 0
    # Power-on contents readable before any write.
    for a in range(4):
        dut.raddr.value = a
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == model[a]
    # Sparse overwrites with interleaved reads.
    dut.we.value = 1
    rng = random.Random(6)
    for _ in range(20):
        a, d, r = rng.randrange(4), rng.randrange(256), rng.randrange(4)
        dut.waddr.value = a
        dut.wdata.value = d
        dut.raddr.value = r
        await FallingEdge(dut.clk)
        model[a] = d
        assert int(dut.result.value) == model[r]
    # Holds under we=0.
    dut.we.value = 0
    dut.wdata.value = 0xAA
    for a in range(4):
        dut.raddr.value = a
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == model[a]


def test_ram():
    simulate("ram", "ram", "test_ram")
