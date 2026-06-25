module add3 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    assign result = (x + 3);
endmodule

module add9 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] x_1;
    add3 add3 (
        .x(x),
        .result(x_1)
    );
    logic [7:0] __call_0;
    add3 add3_1 (
        .x(x_1),
        .result(__call_0)
    );
    add3 add3_2 (
        .x(__call_0),
        .result(result)
    );
endmodule
