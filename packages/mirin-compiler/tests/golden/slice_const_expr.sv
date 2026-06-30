module pick (
    input  logic [15:0] v,
    output logic [3:0] result
);
    logic [15:0] __inl0__self;
    assign __inl0__self = v;
    logic [3:0] __inl0____block_0;
    assign __inl0____block_0 = __inl0__self[4 +: 4];
    assign result = __inl0____block_0;
endmodule
