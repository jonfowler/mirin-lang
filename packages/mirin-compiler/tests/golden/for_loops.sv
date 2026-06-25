module scale_all (
    input  logic clk,
    input  logic [7:0] v [0:2],
    input  logic [7:0] k,
    output logic [7:0] result
);
    logic [7:0] outs [0:2];
    for (genvar i = 0; i < 3; i++) begin : g_x
        logic [7:0] x;
        assign x = v[i];
        assign outs[i] = (x * k);
    end
    assign result = ((outs[0] + outs[1]) + outs[2]);
endmodule

module popcount_ish (
    input  logic clk,
    input  logic [3:0] raw,
    output logic [3:0] result
);
    logic [3:0] hits [0:3];
    for (genvar i = 0; i < 4; i++) begin : g_b
        logic b;
        assign b = raw[i];
        logic [3:0] __block_0;
        always_comb begin
            if (b) begin
                __block_0 = 1;
            end else begin
                __block_0 = 0;
            end
        end
        assign hits[i] = __block_0;
    end
    assign result = (((hits[0] + hits[1]) + hits[2]) + hits[3]);
endmodule
