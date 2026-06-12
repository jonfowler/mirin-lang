"""counter.mrn: parametric-width free-running counter. Built at two widths to
check the SV parameter (Mirin const generic) actually elaborates."""

import cocotb
import pytest
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge

from harness import simulate


@cocotb.test()
async def counts_and_wraps(dut):
    Clock(dut.clk, 10, unit="ns").start()
    wrap = 1 << len(dut.result)
    dut.rstn.value = 0
    await FallingEdge(dut.clk)
    await FallingEdge(dut.clk)
    assert int(dut.result.value) == 0

    dut.rstn.value = 1
    for i in range(2 * wrap + 5):
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == (i + 1) % wrap


@pytest.mark.parametrize("bits", [3, 8])
def test_counter(bits):
    simulate("counter", "counter", "test_counter", parameters={"bits": bits})
