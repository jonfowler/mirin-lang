module build (
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic [7:0] result [0:3]
);
    logic [7:0] v [0:3];
    assign v[0:1] = '{a, b};
    assign v[2:3] = '{b, a};
    assign result = v;
endmodule
