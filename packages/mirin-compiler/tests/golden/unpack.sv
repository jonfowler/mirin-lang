module as_uint (
    input  logic clk,
    input  logic [7:0] b,
    output logic [7:0] result
);
    assign result = (b);
endmodule

module as_sint (
    input  logic clk,
    input  logic [7:0] b,
    output logic signed [7:0] result
);
    assign result = (b);
endmodule

module as_bits (
    input  logic clk,
    input  logic [3:0] b,
    output logic [3:0] result
);
    assign result = (b);
endmodule

module as_bool (
    input  logic clk,
    input  logic [0:0] b,
    output logic result
);
    assign result = (b);
endmodule
