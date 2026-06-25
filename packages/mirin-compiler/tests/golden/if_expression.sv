module pickOne (
    input  logic [7:0] a,
    input  logic [7:0] b,
    input  logic cond,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    always_comb begin
        if (cond) begin
            __block_0 = a;
        end else begin
            __block_0 = b;
        end
    end
    assign result = __block_0;
endmodule
