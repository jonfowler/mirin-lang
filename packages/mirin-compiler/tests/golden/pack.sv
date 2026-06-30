module pack_byte (
    input  logic clk,
    input  logic [7:0] a,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = a;
    assign result = __block_0;
endmodule

module pack_signed (
    input  logic clk,
    input  logic signed [7:0] a,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = a;
    assign result = __block_0;
endmodule

module pack_flag (
    input  logic clk,
    input  logic f,
    output logic [0:0] result
);
    logic [0:0] __block_0;
    assign __block_0 = f;
    assign result = __block_0;
endmodule
