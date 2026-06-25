module pipeline (
    input  logic clk,
    input  logic rstn,
    input  logic [7:0] data,
    output logic [7:0] result
);
    logic [7:0] data_1;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            data_1 <= 0;
        end else begin
            data_1 <= (data + 1);
        end
    end
    logic [7:0] data_2;
    always_ff @(posedge clk) begin
        if (!rstn) begin
            data_2 <= 0;
        end else begin
            data_2 <= (data_1 * 2);
        end
    end
    assign result = data_2;
endmodule
