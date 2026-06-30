module top (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = a + a;
    assign result = (__block_0 + b);
endmodule
