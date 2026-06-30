module s (
    input  logic [3:0] v [0:7],
    output logic [3:0] result [0:2]
);
    logic [3:0] __inl0__self [0:7];
    assign __inl0__self = v;
    assign result = __inl0__self[2 +: 3];
endmodule
