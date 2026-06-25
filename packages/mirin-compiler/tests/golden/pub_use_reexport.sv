module add3 (
    input  logic [7:0] x,
    output logic [7:0] result
);
    assign result = (x + 3);
endmodule

module scale (
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] __call_0;
    add3 add3 (
        .x(x),
        .result(__call_0)
    );
    logic [7:0] __call_1;
    add3 add3_1 (
        .x(x),
        .result(__call_1)
    );
    assign result = (__call_0 + __call_1);
endmodule

module top (
    input  logic [7:0] x,
    output logic [7:0] result
);
    scale scale (
        .x(x),
        .result(result)
    );
endmodule
