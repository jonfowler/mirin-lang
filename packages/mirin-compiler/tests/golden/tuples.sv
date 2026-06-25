module pairSwap (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [3:0] b,
    output logic [3:0] result__0,
    output logic [7:0] result__1
);
    logic [7:0] p__0;
    logic [3:0] p__1;
    assign p__0 = a;
    assign p__1 = b;
    logic [7:0] x;
    assign x = p__0;
    logic [3:0] y;
    assign y = p__1;
    assign result__0 = y;
    assign result__1 = ((p__0 + x) - x);
endmodule
