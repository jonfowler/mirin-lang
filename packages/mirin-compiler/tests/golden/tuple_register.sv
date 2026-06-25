module regPair (
    input  logic clk,
    input  logic [7:0] a,
    input  logic b,
    output logic [7:0] result__0,
    output logic result__1
);
    logic [7:0] r__0;
    logic r__1;
    always_ff @(posedge clk) begin
        r__0 <= a;
        r__1 <= b;
    end
    assign result__0 = r__0;
    assign result__1 = r__1;
endmodule
