module s (
    input  logic [15:0] x,
    input  logic [3:0] i,
    output logic [3:0] result
);
    logic [15:0] __inl0__self;
    assign __inl0__self = x;
    logic [3:0] __inl0__lo;
    assign __inl0__lo = i;
    assign result = (__inl0__self[__inl0__lo +: 4]);
endmodule
