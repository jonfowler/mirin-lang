module add3 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    assign result = (x + 3);
endmodule

module top (
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] __call_0;
    add3 add3 (
        .x(x),
        .result(__call_0)
    );
    add3 add3_1 (
        .x(__call_0),
        .result(result)
    );
endmodule
