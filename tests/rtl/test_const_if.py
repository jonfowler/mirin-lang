"""const_if.mrn: a `const if` folds at elaboration to one arm. `pick_a` has a
true literal condition so it must wire `result = a`; `pick_b` has a false one so
it wires `result = b`. This checks the fold picked the right arm (no mux)."""

import random

import cocotb
from cocotb.triggers import Timer

from harness import simulate


@cocotb.test()
async def pick_a_is_a(dut):
    rng = random.Random(3)
    for _ in range(50):
        a, b = rng.randrange(256), rng.randrange(256)
        dut.a.value = a
        dut.b.value = b
        await Timer(1, unit="ns")
        assert int(dut.result.value) == a


def test_const_if():
    simulate("const_if", "pick_a", "test_const_if")
