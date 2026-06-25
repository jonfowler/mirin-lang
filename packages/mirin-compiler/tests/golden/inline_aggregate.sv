module top (
    input  logic clk,
    input  logic [7:0] inp__a,
    input  logic [7:0] inp__b,
    output logic [7:0] result__a,
    output logic [7:0] result__b
);
    logic [7:0] __inl0__p__a;
    assign __inl0__p__a = inp__a;
    logic [7:0] __inl0__p__b;
    assign __inl0__p__b = inp__b;
    assign result__a = __inl0__p__b;
    assign result__b = __inl0__p__a;
endmodule
