"""param_instance.plr: result = reg(x + 1) through width-generic instances —
exercises the `#(.n(8))` instance parameter bindings end to end."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

from harness import simulate


@cocotb.test()
async def registered_increment(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.x.value = 0
    await FallingEdge(dut.clk)

    rng = random.Random(3)
    for _ in range(50):
        x = rng.randrange(256)
        dut.x.value = x
        await RisingEdge(dut.clk)
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == (x + 1) % 256


def test_param_instance():
    simulate("param_instance", "top", "test_param_instance")
