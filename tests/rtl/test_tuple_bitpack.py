"""tuple_bitpack.mrn: BitPack round trip for a (uint(8), uint(4), bool) tuple.
`pack` concatenates the elements little-endian (element 0 in the low bits) and
`unpack` reads the same layout back, so `unpack(pack(x)) == x` for every input."""

import random

import cocotb
from cocotb.triggers import Timer

from harness import simulate


@cocotb.test()
async def pack_unpack_roundtrip(dut):
    rng = random.Random(7)
    for _ in range(200):
        a, b, c = rng.randrange(256), rng.randrange(16), rng.randrange(2)
        dut.a.value = a
        dut.b.value = b
        dut.c.value = c
        await Timer(1, unit="ns")
        assert int(dut.result__0.value) == a
        assert int(dut.result__1.value) == b
        assert int(dut.result__2.value) == c


def test_tuple_bitpack():
    simulate("tuple_bitpack", "roundtrip3", "test_tuple_bitpack")
