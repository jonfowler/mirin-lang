module drop_all (
    input  logic [7:0] v [0:3]
);
endmodule

module use_zero (
    input  logic [7:0] v [0:3]
);
    logic [7:0] __inl0__v [0:3];
    assign __inl0__v = v;
endmodule

module use_two (
    input  logic [7:0] v [0:3],
    output logic [7:0] result [0:1]
);
    logic [7:0] __inl0__v [0:3];
    assign __inl0__v = v;
    assign result = __inl0__v[0 +: 2];
endmodule
