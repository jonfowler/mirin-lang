module pack_byte (
    input  logic clk,
    input  logic [7:0] a,
    output logic [7:0] result
);
    assign result = (a);
endmodule

module pack_signed (
    input  logic clk,
    input  logic signed [7:0] a,
    output logic [7:0] result
);
    assign result = (a);
endmodule

module pack_flag (
    input  logic clk,
    input  logic f,
    output logic [0:0] result
);
    assign result = (f);
endmodule
