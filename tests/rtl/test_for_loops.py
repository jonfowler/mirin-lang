"""for_loops.mrn: generate-for replication — scale_all computes
sum(v[i] * k) through three generate-block instances of the body."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge

from harness import simulate


@cocotb.test()
async def scales_and_sums(dut):
    Clock(dut.clk, 10, unit="ns").start()
    rng = random.Random(4)
    for _ in range(30):
        vs = [rng.randrange(256) for _ in range(3)]
        k = rng.randrange(256)
        for i, val in enumerate(vs):
            dut.v[i].value = val
        dut.k.value = k
        await FallingEdge(dut.clk)
        expect = sum((x * k) % 256 for x in vs) % 256
        assert int(dut.result.value) == expect


def test_for_loops():
    simulate("for_loops", "scale_all", "test_for_loops")
