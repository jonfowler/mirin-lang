module top (
    input  logic [7:0] a,
    output logic [7:0] result
);
    logic [7:0] __inl0__a;
    assign __inl0__a = a;
    logic [7:0] __inl0__x;
    assign __inl0__x = (__inl0__a + __inl0__a);
    assign result = __inl0__x;
endmodule
