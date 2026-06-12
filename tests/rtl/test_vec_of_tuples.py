"""vec_of_tuples.mrn: Vec(3, (bool, uint(8))) flattened struct-of-arrays;
the enumerate loop sums the valid elements' data."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge

from harness import simulate


@cocotb.test()
async def sums_valid_elements(dut):
    Clock(dut.clk, 10, unit="ns").start()
    rng = random.Random(3)
    for _ in range(40):
        valids = [rng.randrange(2) for _ in range(3)]
        datas = [rng.randrange(256) for _ in range(3)]
        for i in range(3):
            dut.ps__0[i].value = valids[i]
            dut.ps__1[i].value = datas[i]
        await FallingEdge(dut.clk)
        expect = sum(d for v, d in zip(valids, datas) if v) % 256
        assert int(dut.result.value) == expect


def test_vec_of_tuples():
    simulate("vec_of_tuples", "pickPairs", "test_vec_of_tuples")
