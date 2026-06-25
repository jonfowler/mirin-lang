module DF__reg_fwd (
    input  logic clk,
    input  logic self__valid,
    input  logic [7:0] self__data,
    output logic self__ready,
    input  logic rst,
    output logic result__valid,
    output logic [7:0] result__data,
    input  logic result__ready
);
    logic en;
    logic reg_vld;
    logic [7:0] reg_data;
    assign en = ((!reg_vld) || result__ready);
    logic __block_1;
    always_comb begin
        if (en) begin
            __block_1 = self__valid;
        end else begin
            __block_1 = reg_vld;
        end
    end
    logic __block_0;
    always_comb begin
        if (rst) begin
            __block_0 = 1'b0;
        end else begin
            __block_0 = __block_1;
        end
    end
    always_ff @(posedge clk) begin
        reg_vld <= __block_0;
    end
    logic [7:0] __block_2;
    always_comb begin
        if (en) begin
            __block_2 = self__data;
        end else begin
            __block_2 = reg_data;
        end
    end
    always_ff @(posedge clk) begin
        reg_data <= __block_2;
    end
    assign self__ready = en;
    assign result__valid = reg_vld;
    assign result__data = reg_data;
endmodule
