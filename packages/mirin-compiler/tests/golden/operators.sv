module clamp (
    input  logic clk,
    input  logic [7:0] x,
    input  logic [7:0] hi,
    output logic [7:0] result
);
    logic over;
    assign over = (hi < x);
    logic [7:0] __block_0;
    always_comb begin
        if (over) begin
            __block_0 = hi;
        end else begin
            __block_0 = x;
        end
    end
    assign result = __block_0;
endmodule

module same (
    input  logic clk,
    input  logic [7:0] a,
    input  logic [7:0] b,
    output logic result
);
    assign result = (a == b);
endmodule
