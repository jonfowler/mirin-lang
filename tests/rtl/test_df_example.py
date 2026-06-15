"""df_example.mrn: `DF::reg_fwd`, a valid/ready forward register built from
the boolean operators (`!`, `||`). Load-enables when the register is empty or
the downstream is taking the word (`en = !reg_vld || result__ready`), drives
that enable back as upstream backpressure (`self__ready = en`), and registers
valid/data one deep. `rst` is active-high (clears `reg_vld`)."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

from harness import simulate


@cocotb.test()
async def forward_register_handshakes(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.rst.value = 1
    dut.self__valid.value = 0
    dut.self__data.value = 0
    dut.result__ready.value = 0
    await FallingEdge(dut.clk)
    await FallingEdge(dut.clk)
    dut.rst.value = 0

    # Mirror of the one-deep register held across the clock edge.
    reg_vld = 0
    reg_data = 0
    rng = random.Random(11)
    for _ in range(200):
        sv = rng.randrange(2)
        sd = rng.randrange(256)
        dn = rng.randrange(2)
        dut.self__valid.value = sv
        dut.self__data.value = sd
        dut.result__ready.value = dn

        # Enable is combinational from the PRE-edge register state.
        en = (not reg_vld) or bool(dn)
        await RisingEdge(dut.clk)
        if en:
            reg_vld = sv
            reg_data = sd
        await FallingEdge(dut.clk)

        # Outputs now reflect the POST-edge state; `dn` is still driven.
        assert int(dut.result__valid.value) == reg_vld
        if reg_vld:
            assert int(dut.result__data.value) == reg_data
        assert int(dut.self__ready.value) == int((not reg_vld) or bool(dn))


def test_df_example():
    simulate("df_example", "DF__reg_fwd", "test_df_example")
