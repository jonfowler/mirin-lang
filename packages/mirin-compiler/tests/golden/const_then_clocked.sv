module double (
    input  logic [7:0] v,
    output logic [7:0] result
);
    assign result = (v + v);
endmodule

module use_both (
    input  logic clk,
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] k;
    double double (
        .v(3),
        .result(k)
    );
    assign result = (x + k);
endmodule
