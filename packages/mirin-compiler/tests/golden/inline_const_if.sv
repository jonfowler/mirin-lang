module top (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result
);
    logic [7:0] __inl0__a;
    assign __inl0__a = a;
    logic [7:0] __inl0__b;
    assign __inl0__b = b;
    logic [7:0] __inl1__a;
    assign __inl1__a = a;
    logic [7:0] __inl1__b;
    assign __inl1__b = b;
    assign result = (__inl0__a + __inl1__b);
endmodule
