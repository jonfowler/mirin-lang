"""tuple_register.mrn: a registered (uint(8), bool) tuple var — both leaves
(r__0, r__1) register on the same posedge."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge

from harness import simulate


@cocotb.test()
async def both_tuple_leaves_register(dut):
    Clock(dut.clk, 10, unit="ns").start()
    rng = random.Random(2)
    prev_a, prev_b = None, None
    for _ in range(30):
        a, b = rng.randrange(256), rng.randrange(2)
        dut.a.value = a
        dut.b.value = b
        await FallingEdge(dut.clk)
        # The values driven BEFORE this edge appear after it.
        assert int(dut.result__0.value) == a
        assert int(dut.result__1.value) == b
        prev_a, prev_b = a, b


def test_tuples():
    simulate("tuple_register", "regPair", "test_tuples")
