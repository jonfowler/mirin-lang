module counter (
    input  logic clk,
    input  logic rst,
    output logic [7:0] result
);
    logic [7:0] count;
    logic [7:0] __block_0;
    always_comb begin
        if (rst) begin
            __block_0 = (count + 1);
        end else begin
            __block_0 = 0;
        end
    end
    always_ff @(posedge clk) begin
        count <= __block_0;
    end
    assign result = count;
endmodule
