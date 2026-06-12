"""mult_add.plr: registered multiply with combinational add of `c`."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

from harness import simulate


@cocotb.test()
async def reset_then_pipeline(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.rstn.value = 0
    dut.a.value = 0
    dut.b.value = 0
    dut.c.value = 0
    await FallingEdge(dut.clk)

    # In reset the product register is held at 0, so result == c.
    dut.c.value = 7
    await FallingEdge(dut.clk)
    assert int(dut.result.value) == 7

    dut.rstn.value = 1
    rng = random.Random(0)
    for _ in range(100):
        a, b, c = rng.randrange(256), rng.randrange(256), rng.randrange(256)
        dut.a.value = a
        dut.b.value = b
        dut.c.value = c
        await RisingEdge(dut.clk)  # product registered here
        await FallingEdge(dut.clk)
        assert int(dut.result.value) == (a * b + c) % 256


def test_mult_add():
    simulate("mult_add", "multAdd", "test_mult_add")
