module band (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = (a & b);
endmodule

module bxor (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = (a ^ b);
endmodule

module bnot (
    input  logic clk,
    input  logic [7:0] a,
    output logic [7:0] result
);
    assign result = (~a);
endmodule

module mixed (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = ((a & (b << 1)) | a);
endmodule
