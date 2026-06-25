module pickPairs (
    input  logic clk,
    input  logic ps__0 [0:2],
    input  logic [7:0] ps__1 [0:2],
    output logic [7:0] result
);
    logic [7:0] acc [0:2];
    for (genvar i = 0; i < 3; i++) begin : g_p
        logic p__0;
        logic [7:0] p__1;
        assign p__0 = ps__0[i];
        assign p__1 = ps__1[i];
        logic valid;
        assign valid = p__0;
        logic [7:0] data;
        assign data = p__1;
        logic [7:0] __block_0;
        always_comb begin
            if (valid) begin
                __block_0 = data;
            end else begin
                __block_0 = 0;
            end
        end
        assign acc[i] = __block_0;
    end
    assign result = ((acc[0] + acc[1]) + acc[2]);
endmodule
