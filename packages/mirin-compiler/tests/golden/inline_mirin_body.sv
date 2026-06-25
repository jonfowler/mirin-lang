module top (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] __inl0__a;
    assign __inl0__a = a;
    logic [7:0] __inl0__one;
    logic [7:0] __inl0____inl0__a;
    assign __inl0____inl0__a = __inl0__a;
    assign __inl0__one = __inl0____inl0__a;
    logic [7:0] __inl1__a;
    assign __inl1__a = b;
    assign result = ((__inl0__one + 1) + __inl1__a);
endmodule
