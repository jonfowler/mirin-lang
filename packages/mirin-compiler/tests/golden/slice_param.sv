module drop_low #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [(n - 1)-1:0] result
);
    logic [n-1:0] __inl0__self;
    assign __inl0__self = x;
    logic [(n - 1)-1:0] __inl0____block_0;
    if (((n - 1) == 0)) begin : __inl0____block_0__g
        logic [(n - 1)-1:0] __inl0____block_1;
        assign __inl0____block_1 = '0;
        assign __inl0____block_0 = __inl0____block_1;
    end else begin : __inl0____block_0__g
        logic [(n - 1)-1:0] __inl0____block_2;
        assign __inl0____block_2 = __inl0__self[1 +: (n - 1)];
        assign __inl0____block_0 = __inl0____block_2;
    end
    assign result = __inl0____block_0;
endmodule
