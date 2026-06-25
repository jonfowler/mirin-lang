module udiv (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = (a / b);
endmodule

module urem (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = (a % b);
endmodule

module sdiv (
    input  logic clk,
    input  logic signed [7:0] a,
    input  logic signed [7:0] b,
    output logic signed [7:0] result
);
    assign result = (a / b);
endmodule
