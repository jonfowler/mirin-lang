"""delay.mrn: four chained reg2 stages (two double_delays). The payload is
registered at every stage; valid passes through combinationally."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

from harness import simulate


@cocotb.test()
async def four_stage_payload_delay(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.rstn.value = 0
    dut.upstream__valid.value = 0
    dut.upstream__payload.value = 0
    await FallingEdge(dut.clk)
    await FallingEdge(dut.clk)
    dut.rstn.value = 1

    sent = []
    rng = random.Random(1)
    for i in range(60):
        p = rng.randrange(256)
        v = i % 2
        dut.upstream__payload.value = p
        dut.upstream__valid.value = v
        sent.append(p)
        await RisingEdge(dut.clk)
        await FallingEdge(dut.clk)
        assert int(dut.downstream__valid.value) == v  # combinational
        expect = sent[i - 3] if i >= 3 else 0  # 4 registers deep
        assert int(dut.downstream__payload.value) == expect


def test_delay():
    simulate("delay", "test_out_binding_named", "test_delay")
