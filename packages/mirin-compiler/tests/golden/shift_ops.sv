module shl_u (
    input  logic clk,
    input  logic [7:0] a,
    output logic [7:0] result
);
    assign result = (a << 2);
endmodule

module shr_u (
    input  logic clk,
    input  logic [7:0] a,
    output logic [7:0] result
);
    assign result = (a >> 3);
endmodule

module shr_bits (
    input  logic clk,
    input  logic [7:0] a,
    output logic [7:0] result
);
    assign result = (a >> 1);
endmodule

module shr_s (
    input  logic clk,
    input  logic signed [7:0] a,
    output logic signed [7:0] result
);
    assign result = (a >>> 2);
endmodule
