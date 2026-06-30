module s #(parameter int w) (
    input  logic [15:0] x,
    input  logic [3:0] i,
    output logic [w-1:0] result
);
    logic [15:0] __inl0__self;
    assign __inl0__self = x;
    logic [3:0] __inl0__lo;
    assign __inl0__lo = i;
    logic [w-1:0] __inl0____block_0;
    if ((w == 0)) begin : __inl0____block_0__g
        logic [w-1:0] __inl0____block_1;
        assign __inl0____block_1 = '0;
        assign __inl0____block_0 = __inl0____block_1;
    end else begin : __inl0____block_0__g
        logic [w-1:0] __inl0____block_2;
        assign __inl0____block_2 = __inl0__self[__inl0__lo +: w];
        assign __inl0____block_0 = __inl0____block_2;
    end
    assign result = __inl0____block_0;
endmodule
