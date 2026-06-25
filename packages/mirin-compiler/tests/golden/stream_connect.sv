module Stream8__connect (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic self__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready
);
    assign downstream__valid = self__valid;
    assign downstream__data = self__data;
    assign self__ready = downstream__ready;
endmodule

module top (
    input  logic clk,
    input  logic upstream__valid,
    input  logic [7:0] upstream__data,
    output logic upstream__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data,
    input  logic downstream__ready
);
    Stream8__connect Stream8__connect (
        .clk(clk),
        .self__valid(upstream__valid),
        .self__data(upstream__data),
        .self__ready(upstream__ready),
        .downstream__valid(downstream__valid),
        .downstream__data(downstream__data),
        .downstream__ready(downstream__ready)
    );
endmodule
