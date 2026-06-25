module top (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = ((a + a) + b);
endmodule
