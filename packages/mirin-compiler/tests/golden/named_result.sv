module swap (
    input  logic [7:0] p__a,
    input  logic [7:0] p__b,
    output logic [7:0] swapped__a,
    output logic [7:0] swapped__b
);
    assign swapped__a = p__b;
    assign swapped__b = p__a;
endmodule

module build (
    input  logic [7:0] x,
    output logic [7:0] built__a,
    output logic [7:0] built__b
);
    assign built__a = x;
    assign built__b = x;
endmodule

module add_flag (
    input  logic [7:0] x,
    input  logic [7:0] y,
    output logic [7:0] sum,
    output logic carry
);
    assign sum = (x + y);
    assign carry = (x == y);
endmodule
