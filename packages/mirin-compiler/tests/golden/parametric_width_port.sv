module pipe (
    input  logic clk,
    output logic upstream__ready,
    input  logic upstream__valid,
    input  logic [7:0] upstream__data,
    input  logic downstream__ready,
    output logic downstream__valid,
    output logic [7:0] downstream__data
);
    assign upstream__ready = downstream__ready;
    assign downstream__valid = upstream__valid;
    assign downstream__data = upstream__data;
endmodule
