module pick (
    input  logic [15:0] v,
    output logic [3:0] result
);
    logic [15:0] __inl0__self;
    assign __inl0__self = v;
    assign result = (__inl0__self[4 +: 4]);
endmodule
