module shiftRegister (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] inp,
    output logic [7:0] result
);
    logic [7:0] stage0;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            stage0 <= 0;
        end else begin
            stage0 <= inp;
        end
    end
    logic [7:0] stage1;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            stage1 <= 0;
        end else begin
            stage1 <= stage0;
        end
    end
    assign result = stage1;
endmodule
