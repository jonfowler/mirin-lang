module hold_one (
    input  logic clk,
    input  logic rstn,
    output logic [7:0] result
);
    logic [7:0] one;
    assign one = 1;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            result <= 0;
        end else begin
            result <= one;
        end
    end
endmodule
