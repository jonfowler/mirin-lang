module widen (
    input  logic clk,
    input  logic [7:0] a,
    output logic [15:0] result
);
    logic [15:0] __block_0;
    assign __block_0 = type(__block_0)'(a);
    assign result = __block_0;
endmodule

module narrow (
    input  logic clk,
    input  logic [7:0] a,
    output logic [3:0] result
);
    logic [3:0] __block_0;
    assign __block_0 = type(__block_0)'(a);
    assign result = __block_0;
endmodule

module sign_widen (
    input  logic clk,
    input  logic signed [7:0] a,
    input  logic signed [15:0] b,
    output logic signed [15:0] result
);
    logic signed [15:0] __block_0;
    assign __block_0 = type(__block_0)'(a);
    assign result = (__block_0 + b);
endmodule

module scale_up (
    input  logic clk,
    input  logic [7:0] a,
    output logic [11:0] result
);
    logic [11:0] __block_0;
    assign __block_0 = (type(__block_0)'(a)) << (12 - 8);
    assign result = __block_0;
endmodule

module scale_down (
    input  logic clk,
    input  logic [7:0] a,
    output logic [3:0] result
);
    logic [3:0] __block_0;
    assign __block_0 = type(__block_0)'(a >> (8 - 4));
    assign result = __block_0;
endmodule

module resize_up (
    input  logic clk,
    input  logic [7:0] a,
    output logic [11:0] result
);
    logic [11:0] __block_0;
    assign __block_0 = type(__block_0)'(a);
    assign result = __block_0;
endmodule

module resize_down (
    input  logic clk,
    input  logic signed [7:0] a,
    output logic signed [3:0] result
);
    logic signed [3:0] __block_0;
    assign __block_0 = type(__block_0)'(a);
    assign result = __block_0;
endmodule
