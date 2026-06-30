module as_uint (
    input  logic clk,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = b;
    assign result = __block_0;
endmodule

module as_sint (
    input  logic clk,
    input  logic [7:0] b,
    output logic signed [7:0] result
);
    logic signed [7:0] __block_0;
    assign __block_0 = b;
    assign result = __block_0;
endmodule

module as_bits (
    input  logic clk,
    input  logic [3:0] b,
    output logic [3:0] result
);
    logic [3:0] __block_0;
    assign __block_0 = b;
    assign result = __block_0;
endmodule

module as_bool (
    input  logic clk,
    input  logic [0:0] b,
    output logic result
);
    logic __block_0;
    assign __block_0 = b;
    assign result = __block_0;
endmodule
