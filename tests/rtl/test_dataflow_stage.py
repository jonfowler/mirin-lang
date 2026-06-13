"""dataflow_stage.mrn: a returned-port pipeline register. Exercises the
returned-port direction fix — result__ready is a module INPUT (downstream
backpressure), up__ready a module OUTPUT. valid/data register one deep;
ready passes back combinationally."""

import random

import cocotb
from cocotb.clock import Clock
from cocotb.triggers import FallingEdge, RisingEdge

from harness import simulate


@cocotb.test()
async def registers_data_and_passes_ready_back(dut):
    Clock(dut.clk, 10, unit="ns").start()
    dut.rstn.value = 0
    dut.up__valid.value = 0
    dut.up__data.value = 0
    dut.result__ready.value = 0
    await FallingEdge(dut.clk)
    await FallingEdge(dut.clk)
    dut.rstn.value = 1

    sent = []
    rng = random.Random(7)
    for i in range(60):
        v = i % 3 != 0
        d = rng.randrange(256)
        rdy = rng.randrange(2)
        dut.up__valid.value = int(v)
        dut.up__data.value = d
        dut.result__ready.value = rdy
        sent.append((int(v), d))
        # ready is combinational backpressure: up__ready == result__ready.
        await RisingEdge(dut.clk)
        await FallingEdge(dut.clk)
        assert int(dut.up__ready.value) == rdy
        # The input driven before this edge is captured and visible now.
        ev, ed = sent[i]
        assert int(dut.result__valid.value) == ev
        assert int(dut.result__data.value) == ed


def test_dataflow_stage():
    simulate("dataflow_stage", "stage", "test_dataflow_stage")
