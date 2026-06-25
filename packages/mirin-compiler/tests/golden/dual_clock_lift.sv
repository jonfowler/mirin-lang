module inc (
    input  logic [7:0] v,
    output logic [7:0] result
);
    assign result = (v + 1);
endmodule

module two_domains (
    input  logic a,
    input  logic b,
    input  logic [7:0] x,
    input  logic [7:0] y,
    output logic [7:0] result
);
    logic [7:0] xs;
    inc inc (
        .v(x),
        .result(xs)
    );
    logic [7:0] ys;
    inc inc_1 (
        .v(y),
        .result(ys)
    );
    assign result = xs;
endmodule
