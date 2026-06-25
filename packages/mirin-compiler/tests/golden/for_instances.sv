module stage (
    input  logic clk,
    input  logic [7:0] x,
    input  logic [7:0] k,
    output logic [7:0] result
);
    assign result = (x * k);
endmodule

module pipeline (
    input  logic clk,
    input  logic [7:0] v [0:2],
    input  logic [7:0] k,
    output logic [7:0] result
);
    logic [7:0] outs [0:2];
    for (genvar i = 0; i < 3; i++) begin : g_x
        logic [7:0] x;
        assign x = v[i];
        logic [7:0] __call_0;
        stage stage (
            .clk(clk),
            .x(x),
            .k(k),
            .result(__call_0)
        );
        assign outs[i] = __call_0;
    end
    assign result = outs[2];
endmodule
