module ne_op (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic result
);
    assign result = (a != b);
endmodule

module le_op (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic result
);
    assign result = (a <= b);
endmodule

module gt_op (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic result
);
    assign result = (a > b);
endmodule

module ge_op (
    input  logic clk,
    input  logic signed [7:0] a,
    input  logic signed [7:0] b,
    output logic result
);
    assign result = (a >= b);
endmodule
