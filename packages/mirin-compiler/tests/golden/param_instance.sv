module scale #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [n-1:0] result
);
    assign result = (x + 1);
    initial begin
        assert (1 < (1 << n));
    end
endmodule

module ff_gen #(parameter int n) (
    input  logic clk,
    input  logic [n-1:0] d,
    output logic [n-1:0] q
);
    always_ff @(posedge clk) q <= d;
endmodule

module wrap #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [n-1:0] result
);
    scale #(
        .n(n)
    ) scale (
        .x(x),
        .result(result)
    );
endmodule

module top (
    input  logic clk,
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] q;
    logic [7:0] __call_0;
    scale #(
        .n(8)
    ) scale (
        .x(x),
        .result(__call_0)
    );
    ff_gen #(
        .n(8)
    ) ff_gen (
        .clk(clk),
        .d(__call_0),
        .q(q)
    );
    assign result = q;
endmodule
